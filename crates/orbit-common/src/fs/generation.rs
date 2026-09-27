//! Cross-process upgrade admission for one authority root.
//!
//! OS locks, not process discovery or expiring leases, define liveness. Never
//! unlink these files: replacing a locked inode would create a second authority.
//!
//! # Compatibility generations (`compatibility-generation-v2`)
//!
//! A generation is a [`CompatibilityIdentity`] — the store schema, workspace
//! layout and feature schema versions a binary was compiled with, and the
//! newest migrations older binaries could not keep reading or writing — not
//! the executable's digest. Every participant holds `.generation.lock` shared
//! for its lifetime. `.generation-compat.json` records the envelope of every
//! identity admitted since the authority last had no participant:
//!
//! - A newcomer compatible with that envelope joins as an ordinary writer (or
//!   reader) whatever its digest, and widens it. A rebuild or patch release
//!   with the same state versions, or one whose newer migrations are all
//!   [additive](../../../orbit_store/contracts/enum.MigrationCompatibility.html),
//!   runs — and migrates — beside the live processes.
//! - An incompatible *newer* writer (a pending breaking migration) records a
//!   [`PendingSwitch`] and waits, up to [`quiesce_bound`], for exclusive
//!   admission. Live participants observe the switch at their safe points and
//!   yield; a newcomer of any other identity is refused while it is pending.
//!   When the bound expires the refusal names every live participant by PID,
//!   role and start time.
//! - An incompatible *older* binary is refused outright: it may not displace
//!   newer participants.
//!
//! Participants register PID, role and start time under
//! `.generation-participants/`, each record held by its own lock.
//!
//! # Yielding and handing over
//!
//! Long-lived participants ([`ParticipantRole::is_long_lived`]) check
//! [`pending_switch_for_this_process`] at their safe points: `mcp serve`
//! between requests, the dashboard between lifecycle ticks, a pipeline worker
//! after each checkpointed step (recording its run interrupted, not failed).
//! Independently, when [`replaced_installation`] reports that the installed
//! executable no longer names their running image, `mcp serve`, the
//! dashboard and drain coordinators ask [`handover_target`] whether the
//! replacement speaks this contract and their [`RESUME_CAPABILITIES`] entry,
//! and [`reexec`] it at an idle boundary. The exec keeps the PID and releases
//! every lock (Orbit descriptors are close-on-exec), so the new image joins
//! like any newcomer.
//!
//! # Coexistence with `executable-generation-v1`
//!
//! `.generation.lock` keeps its v1 content (`1:<sha256>\n`), and every v2
//! participant holds it shared, so a v1 process still refuses to write, and a
//! v1 `orbit update` still refuses to replace the executable, while any v2
//! process is live. The v2 record is authoritative only while its
//! `record_digest` equals that v1 content: a v1 takeover rewrites the content,
//! which invalidates it. A v2 newcomer that finds a v1-owned record falls back
//! to v1 rules — exact digest, or exclusive takeover, otherwise refused
//! immediately (v1 processes never observe a pending switch, so waiting for
//! them would only stall). [`GenerationGuard::acquire`] pins an exact digest
//! under v1 rules alone, as a v1 process would.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::OrbitError;

mod handoff;
mod identity;
mod registry;

pub use handoff::{
    RESUME_CAPABILITIES, RESUME_DRAIN_ADOPT, RESUME_MCP_STDIO, candidate_supports, handover_target,
    reexec, replaced_installation,
};
pub use identity::{Access, CompatibilityIdentity, LedgerCompatibility};
pub use registry::{ParticipantRecord, ParticipantRole, PendingSwitch, pending_switch};

use identity::Envelope;
use registry::{PendingClaim, Registration};

/// Admission protocol this binary implements.
pub const GENERATION_CONTRACT: &str = "compatibility-generation-v2";

/// The executable-digest protocol every v2 binary still honours toward v1
/// participants and v1 updaters.
pub const LEGACY_GENERATION_CONTRACT: &str = "executable-generation-v1";

/// Environment variable bounding how long a breaking upgrade waits for live
/// participants to yield, in seconds.
pub const QUIESCE_TIMEOUT_ENV: &str = "ORBIT_UPGRADE_QUIESCE_SECS";

/// Default [`QUIESCE_TIMEOUT_ENV`].
pub const DEFAULT_QUIESCE_TIMEOUT: Duration = Duration::from_secs(120);

const QUIESCE_POLL: Duration = Duration::from_millis(200);

/// A shared generation pin. Retain until all operations and replies finish.
pub struct GenerationGuard {
    _record: Record,
    /// True when this process joined a recorded generation other than its own
    /// digest, without rewriting the record (read-only same-schema join).
    joined_foreign: bool,
    _registration: Option<Registration>,
}

/// Exclusive admission, before installing a candidate or mutating resources.
pub struct GenerationUpdate {
    root: PathBuf,
    generation: Record,
    admission: Record,
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

const QUIESCE: &str = "Quiesce the existing Orbit processes through their owning clients, \
     then retry. Do not delete admission files or replay a mutation whose reply was lost";

/// Remedy for a record this process can read but never rewrite.
const UNWRITABLE: &str = "Run the recorded generation, or retry where the Orbit root is writable. \
     A read-only mount, or a sandbox that denies writes under this root, \
     cannot record a takeover";

const WRITES_WHILE_FOREIGN: &str = "another executable generation is still running \
     (this command writes; read-only commands are admitted when the store schema matches)";

const SWITCH_PENDING: &str = "a generation switch is pending";

const ADMISSION_LOCK: &str = ".generation-admission.lock";
const GENERATION_LOCK: &str = ".generation.lock";
const COMPAT_RECORD: &str = ".generation-compat.json";
const CLOCK_HOLD: &str = ".generation-clock-hold.json";

#[derive(Serialize, Deserialize)]
struct ClockHold {
    digest: String,
    started_at: DateTime<Utc>,
    last_refused_at: DateTime<Utc>,
    refused_ticks: u64,
}

/// True only for a clock writer held back by other live generations: a
/// still-pinned v1 generation, or a pending switch it is not part of.
pub fn is_clock_generation_hold(error: &OrbitError) -> bool {
    let message = error.to_string();
    message.contains(WRITES_WHILE_FOREIGN) || message.contains(SWITCH_PENDING)
}

/// Persist consecutive refused ticks without emitting one error per process.
pub fn record_clock_generation_hold(
    root: &Path,
    digest: &str,
    at: DateTime<Utc>,
) -> Result<(), OrbitError> {
    with_clock_hold(root, |hold| {
        match hold {
            Some(active) if active.digest == digest => {
                active.last_refused_at = at;
                active.refused_ticks = active.refused_ticks.saturating_add(1);
            }
            _ => {
                *hold = Some(ClockHold {
                    digest: digest.to_string(),
                    started_at: at,
                    last_refused_at: at,
                    refused_ticks: 1,
                });
            }
        }
        Ok(())
    })
}

/// Return one dated summary when the refused generation can run again.
pub fn finish_clock_generation_hold(
    root: &Path,
    digest: &str,
    at: DateTime<Utc>,
) -> Result<Option<String>, OrbitError> {
    with_clock_hold(root, |hold| {
        let Some(active) = hold.as_ref().filter(|active| active.digest == digest) else {
            return Ok(None);
        };
        let summary = format!(
            "clock executable generation changed under live Orbit processes (possibly drain workers): started_at={} ended_at={} last_refused_at={} refused_ticks={}",
            active.started_at.to_rfc3339(),
            at.to_rfc3339(),
            active.last_refused_at.to_rfc3339(),
            active.refused_ticks,
        );
        *hold = None;
        Ok(Some(summary))
    })
}

fn with_clock_hold<T>(
    root: &Path,
    change: impl FnOnce(&mut Option<ClockHold>) -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    let path = validated_generation_root(root)?.join(CLOCK_HOLD);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| OrbitError::Io(format!("open clock hold record: {error}")))?;
    FileExt::lock_exclusive(&file)
        .map_err(|error| OrbitError::Io(format!("lock clock hold record: {error}")))?;
    if file
        .metadata()
        .map_err(|error| OrbitError::Io(error.to_string()))?
        .len()
        > 1024
    {
        return Err(OrbitError::InvalidInput(
            "clock hold record exceeds 1024 bytes".into(),
        ));
    }
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .map_err(|error| OrbitError::Io(format!("read clock hold record: {error}")))?;
    let mut hold = if raw.is_empty() {
        None
    } else {
        Some(serde_json::from_str(&raw).map_err(|error| {
            OrbitError::InvalidInput(format!("invalid clock hold record: {error}"))
        })?)
    };
    let result = change(&mut hold)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| OrbitError::Io(error.to_string()))?;
    let encoded = match hold {
        Some(hold) => serde_json::to_vec(&hold)
            .map_err(|error| OrbitError::Execution(format!("encode clock hold record: {error}")))?,
        None => Vec::new(),
    };
    file.write_all(&encoded)
        .map_err(|error| OrbitError::Io(format!("write clock hold record: {error}")))?;
    file.set_len(encoded.len() as u64)
        .map_err(|error| OrbitError::Io(format!("truncate clock hold record: {error}")))?;
    file.sync_data()
        .map_err(|error| OrbitError::Io(format!("sync clock hold record: {error}")))?;
    Ok(result)
}

/// The configured wait for a breaking upgrade (see [`QUIESCE_TIMEOUT_ENV`]).
pub fn quiesce_bound() -> Duration {
    std::env::var(QUIESCE_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map_or(DEFAULT_QUIESCE_TIMEOUT, Duration::from_secs)
}

pub(crate) fn refusal(detail: impl std::fmt::Display) -> OrbitError {
    refused(detail, QUIESCE)
}

fn unwritable(detail: impl std::fmt::Display) -> OrbitError {
    refused(detail, UNWRITABLE)
}

fn refused(detail: impl std::fmt::Display, remedy: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "upgrade admission refused: {detail}; leave the installation and stores unchanged. \
         {remedy}"
    ))
}

fn generation_record_name(name: &str) -> Result<&'static str, OrbitError> {
    match name {
        ADMISSION_LOCK => Ok(ADMISSION_LOCK),
        GENERATION_LOCK => Ok(GENERATION_LOCK),
        COMPAT_RECORD => Ok(COMPAT_RECORD),
        _ => Err(refusal("invalid generation record name")),
    }
}

/// CodeQL `rust/path-injection` treats `Path::starts_with` as a SafeAccessCheck
/// on the receiver. Call this after reconstructing a path so `is_dir` / open
/// sinks only see a prefix-checked value.
fn generation_path_is_contained(path: &Path, base: &Path) -> bool {
    path.starts_with(base)
}

fn generation_leaf_name(root: &Path) -> Result<&OsStr, OrbitError> {
    let Some(name) = root.file_name() else {
        return Err(refusal("generation root must not be empty"));
    };
    if name == "." || name == ".." {
        return Err(refusal("generation root escapes its start"));
    }
    Ok(name)
}

fn contained_under_parent(parent: &Path, name: &OsStr) -> Result<PathBuf, OrbitError> {
    let contained = parent.join(name);
    if !generation_path_is_contained(&contained, parent) {
        return Err(refusal("generation root escapes its parent"));
    }
    Ok(contained)
}

/// Resolve the authority root before any generation lock is created or opened.
///
/// Callers pass `~/.orbit` or a test directory; both are untrusted path values.
/// An existing root is canonicalized so aliases collapse to one directory, then
/// reconstructed under its canonical parent so later filesystem sinks only see
/// a prefix-checked path. A missing root whose parent exists is joined onto
/// that canonical parent. A missing parent is reconstructed from components so
/// `..` cannot walk outside the starting location before `create_dir_all`.
pub(crate) fn validated_generation_root(root: &Path) -> Result<PathBuf, OrbitError> {
    if root.as_os_str().is_empty() {
        return Err(refusal("generation root must not be empty"));
    }
    match root.canonicalize() {
        Ok(canonical) => existing_generation_root(canonical),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => missing_generation_root(root),
        Err(error) => Err(refusal(error)),
    }
}

fn existing_generation_root(canonical: PathBuf) -> Result<PathBuf, OrbitError> {
    let name = generation_leaf_name(&canonical)?;
    let parent = canonical
        .parent()
        .ok_or_else(|| refusal("generation root must be a directory"))?;
    let contained = parent.join(name);
    if !contained.starts_with(parent) {
        return Err(refusal("generation root escapes its parent"));
    }
    if !contained.is_dir() {
        return Err(refusal("generation root must be a directory"));
    }
    Ok(contained)
}

fn missing_generation_root(root: &Path) -> Result<PathBuf, OrbitError> {
    let name = generation_leaf_name(root)?;
    let Some(parent) = root.parent().filter(|path| !path.as_os_str().is_empty()) else {
        return normalize_missing_generation_root(root);
    };
    match parent.canonicalize() {
        Ok(canonical_parent) => contained_under_parent(&canonical_parent, name),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            normalize_missing_generation_root(root)
        }
        Err(error) => Err(refusal(error)),
    }
}

fn normalize_missing_generation_root(root: &Path) -> Result<PathBuf, OrbitError> {
    let mut normalized = PathBuf::new();
    for component in root.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    normalized.pop();
                } else {
                    return Err(refusal("generation root escapes its start"));
                }
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(refusal("generation root must not be empty"));
    }
    match (normalized.parent(), normalized.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
            contained_under_parent(parent, name)
        }
        _ => Ok(normalized),
    }
}

/// The directory whose generation records identify one authority.
///
/// Two spellings of the same authority — `~/.orbit` and a `--root` naming it
/// through a symlink, say — collapse to one value here. A caller that admits
/// against several roots at once must compare them this way before locking:
/// flock treats a second open of the same file as a foreign holder, so locking
/// one authority twice would refuse the update against itself.
pub fn authority_root(root: &Path) -> Result<PathBuf, OrbitError> {
    validated_generation_root(root)
}

/// Join an allow-listed generation record name onto a validated root.
///
/// The original caller string never reaches `Path::join`; only the matching
/// static name does. Containment is re-checked with `starts_with` after the
/// join so the open and `create_dir_all` sinks receive a reconstructed path
/// rather than the user-provided values.
fn validated_generation_record_path(root: &Path, name: &str) -> Result<PathBuf, OrbitError> {
    let root = validated_generation_root(root)?;
    let name = generation_record_name(name)?;
    let path = root.join(name);
    if !generation_path_is_contained(&path, &root) {
        return Err(refusal("generation record path escapes the root"));
    }
    if path.parent() != Some(root.as_path()) {
        return Err(refusal("generation record path escapes the root"));
    }
    Ok(path)
}

/// An admission file and whether this process may rewrite its bytes.
///
/// A participant can join an existing generation from a read-only mount, or
/// from a sandboxed child denied writes under the authority root: flock needs
/// a descriptor, not permission to rewrite bytes. Such a descriptor can never
/// record a takeover, so writability travels with the open instead of
/// surfacing as `write` failing with `EBADF` half way through the protocol.
struct Record {
    file: File,
    writable: bool,
}

impl Drop for Record {
    fn drop(&mut self) {
        // A child forked before exec can inherit this open description. Unlock
        // explicitly so it cannot extend admission or an abandoned update's
        // generation lock after the owner drops its record. Independent
        // participants use independent descriptions and retain their locks.
        let _ = FileExt::unlock(&self.file);
    }
}

fn open(root: &Path, name: &str) -> Result<Record, OrbitError> {
    let root = validated_generation_root(root)?;
    let path = validated_generation_record_path(&root, name)?;
    let parent = root
        .parent()
        .ok_or_else(|| refusal("generation root must be a directory"))?;
    if !root.starts_with(parent) || !path.starts_with(&root) {
        return Err(refusal("generation record path escapes the root"));
    }
    std::fs::create_dir_all(&root).map_err(refusal)?;
    match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
    {
        Ok(file) => Ok(Record {
            file,
            writable: true,
        }),
        Err(error) => File::open(&path)
            .map(|file| Record {
                file,
                writable: false,
            })
            // The read-only retry only explains its own failure. Report why
            // the participating open was refused — and since not even an
            // absent record could be created here, the root's writability is
            // the remedy rather than quiescing processes.
            .map_err(|_| unwritable(format!("the generation record cannot be opened ({error})"))),
    }
}

fn admission(root: &Path) -> Result<Record, OrbitError> {
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

/// SHA-256 of the executable bytes, including checkout builds with equal versions.
pub fn executable_generation(path: &Path) -> Result<String, OrbitError> {
    hash_executable(File::open(path).map_err(refusal)?)
}

fn hash_executable(mut file: File) -> Result<String, OrbitError> {
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(refusal)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Digest the running inode, even after its installed path has been replaced.
pub fn process_generation() -> Result<&'static str, OrbitError> {
    static DIGEST: OnceLock<Result<String, String>> = OnceLock::new();
    DIGEST
        .get_or_init(|| {
            #[cfg(target_os = "linux")]
            let path = Ok::<_, std::io::Error>(PathBuf::from("/proc/self/exe"));
            #[cfg(not(target_os = "linux"))]
            let path = std::env::current_exe();
            let result = (|| {
                let mut file = File::open(path.map_err(refusal)?).map_err(refusal)?;
                verify_running_image(&mut file)?;
                hash_executable(file)
            })();
            result.map_err(|e: OrbitError| e.to_string())
        })
        .as_deref()
        .map_err(refusal)
}

fn read_generation(file: &mut File) -> Result<String, OrbitError> {
    file.seek(SeekFrom::Start(0)).map_err(refusal)?;
    let mut record = String::new();
    file.take(128)
        .read_to_string(&mut record)
        .map_err(refusal)?;
    if record.is_empty() {
        return Ok(record);
    }
    let digest = record.strip_prefix("1:").and_then(|s| s.strip_suffix('\n'));
    match digest {
        Some(s) if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) => Ok(s.into()),
        _ => Err(refusal("invalid generation record")),
    }
}

/// The v2 record beside `.generation.lock`: which identities the live
/// participants were admitted with. Valid only while `record_digest` equals
/// the digest in `.generation.lock`.
#[derive(Serialize, Deserialize)]
struct CompatRecord {
    contract: String,
    record_digest: String,
    envelope: Envelope,
}

fn read_compat(root: &Path, recorded: &str) -> Option<Envelope> {
    if recorded.is_empty() {
        return None;
    }
    let path = validated_generation_record_path(root, COMPAT_RECORD).ok()?;
    let file = File::open(&path).ok()?;
    let mut raw = String::new();
    file.take(16 * 1024).read_to_string(&mut raw).ok()?;
    let record: CompatRecord = serde_json::from_str(&raw).ok()?;
    (record.contract == GENERATION_CONTRACT && record.record_digest == recorded)
        .then_some(record.envelope)
}

fn write_compat(root: &Path, digest: &str, envelope: &Envelope) -> Result<(), OrbitError> {
    let path = validated_generation_record_path(root, COMPAT_RECORD)?;
    let encoded = serde_json::to_string(&CompatRecord {
        contract: GENERATION_CONTRACT.to_string(),
        record_digest: digest.to_string(),
        envelope: envelope.clone(),
    })
    .map_err(refusal)?;
    crate::fs::io::atomic_write_text(&path, &encoded).map_err(unwritable)
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
    fn holding(record: Record, joined_foreign: bool) -> Self {
        Self {
            _record: record,
            joined_foreign,
            _registration: None,
        }
    }

    fn registered(mut self, root: &Path, participant: &Participant<'_>) -> Self {
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
            digest: process_generation()?,
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

fn quiesce_timeout(reason: &str, bound: Duration, blockers: &[ParticipantRecord]) -> OrbitError {
    refusal(format!(
        "a breaking migration is waiting ({reason}), and these Orbit processes did not yield \
         within {}s: {}",
        bound.as_secs(),
        describe_blockers(blockers)
    ))
}

fn describe_blockers(blockers: &[ParticipantRecord]) -> String {
    const UNREGISTERED: &str = "processes that did not register (executable-generation-v1 \
         binaries, or sandboxed children that cannot write the Orbit root)";
    if blockers.is_empty() {
        return UNREGISTERED.to_string();
    }
    let mut listed = blockers
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    listed.push_str(&format!(", and any {UNREGISTERED}"));
    listed
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
    fn pin_as(
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

// Linux opens the running inode directly. Other supported systems must verify
// that current_exe still names the loaded image, before hashing that descriptor.
#[cfg(not(target_os = "macos"))]
fn verify_running_image(_file: &mut File) -> Result<(), OrbitError> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_running_image(file: &mut File) -> Result<(), OrbitError> {
    unsafe extern "C" {
        fn _dyld_get_image_header(index: u32) -> *const u8;
    }
    // SAFETY: dyld's main-image header and load commands remain mapped for
    // this process lifetime. Lengths are bounded before constructing slices.
    let loaded = unsafe { _dyld_get_image_header(0) };
    if loaded.is_null() {
        return Err(refusal("cannot identify the running Mach-O image"));
    }
    let header = unsafe { std::slice::from_raw_parts(loaded, 32) };
    let length = macho_commands_length(header)?;
    let commands = unsafe { std::slice::from_raw_parts(loaded.add(32), length) };
    let running_uuid = macho_uuid(commands)?;
    let mut disk_header = [0u8; 32];
    file.read_exact(&mut disk_header).map_err(refusal)?;
    let mut disk_commands = vec![0; macho_commands_length(&disk_header)?];
    file.read_exact(&mut disk_commands).map_err(refusal)?;
    if running_uuid != macho_uuid(&disk_commands)? {
        return Err(refusal(
            "the installed executable no longer names this running image",
        ));
    }
    file.seek(SeekFrom::Start(0)).map_err(refusal)?;
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn macho_commands_length(header: &[u8]) -> Result<usize, OrbitError> {
    if header.len() != 32 || header[..4] != [0xcf, 0xfa, 0xed, 0xfe] {
        return Err(refusal("expected a native 64-bit Mach-O executable"));
    }
    let size = u32::from_le_bytes([header[20], header[21], header[22], header[23]]) as usize;
    if size > 1024 * 1024 {
        return Err(refusal("invalid Mach-O load commands"));
    }
    Ok(size)
}

#[cfg(any(target_os = "macos", test))]
fn macho_uuid(mut commands: &[u8]) -> Result<[u8; 16], OrbitError> {
    while commands.len() >= 8 {
        let kind = u32::from_le_bytes([commands[0], commands[1], commands[2], commands[3]]);
        let size =
            u32::from_le_bytes([commands[4], commands[5], commands[6], commands[7]]) as usize;
        if size < 8 || size > commands.len() {
            break;
        }
        if kind == 0x1b && size == 24 {
            let mut uuid = [0; 16];
            uuid.copy_from_slice(&commands[8..24]);
            return Ok(uuid);
        }
        commands = &commands[size..];
    }
    Err(refusal("missing or invalid Mach-O image UUID"))
}

#[cfg(test)]
#[path = "tests/generation_image.rs"]
mod image_tests;
