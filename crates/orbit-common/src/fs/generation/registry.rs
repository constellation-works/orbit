//! Who holds an authority, and whether a generation switch is waiting.
//!
//! Every participant that can write under the authority root registers a
//! record naming its PID, role and start time, and holds an exclusive lock on
//! it for its lifetime. The lock, not the file, is liveness: a record whose
//! lock can be taken belongs to a process that has exited, and admission
//! removes it. Records are advisory — they name blockers and let a newer
//! binary ask live processes to yield — while the generation lock itself still
//! decides who may take exclusive admission.
//!
//! A binary that needs exclusive admission for a breaking migration records a
//! pending switch and holds its lock while it waits. Live participants observe
//! it at their safe points; newcomers of any other identity are refused until
//! it resolves.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::identity::{Access, CompatibilityIdentity};
use super::paths::validated_generation_root;
use super::refusal::refusal;
use crate::OrbitError;

const PARTICIPANTS_DIR: &str = ".generation-participants";
const PENDING_RECORD: &str = ".generation-pending.json";
/// A registration being written, before it is renamed into place.
const STAGED_EXTENSION: &str = "staged";
/// How long a staged registration may stay unlocked before it is abandoned.
const STAGED_GRACE: Duration = Duration::from_secs(60);
/// How long a pending claim retries past a reader probing the record.
const CLAIM_SETTLE: Duration = Duration::from_millis(20);
/// Records are small JSON; anything larger is not one of ours.
const MAX_RECORD_BYTES: u64 = 16 * 1024;

/// What a participating process is, for blocker reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantRole {
    /// A one-shot command.
    Command,
    /// A persistent `orbit mcp serve` process.
    McpServe,
    /// The `orbit mcp listen` TCP listener.
    McpListen,
    /// The `orbit web serve` dashboard.
    Dashboard,
    /// A drain coordinator or pipeline worker.
    Drain,
    /// A scheduler clock tick.
    Clock,
}

impl ParticipantRole {
    /// Whether this process stays up across many operations and so must
    /// yield to a pending switch on its own.
    pub fn is_long_lived(self) -> bool {
        matches!(self, Self::McpServe | Self::Dashboard | Self::Drain)
    }

    /// Whether this process finishes on its own within one operation, so an
    /// updater waits for it instead of refusing.
    pub fn is_short_lived(self) -> bool {
        matches!(self, Self::Command | Self::Clock)
    }
}

impl fmt::Display for ParticipantRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Command => "command",
            Self::McpServe => "mcp serve",
            Self::McpListen => "mcp listen",
            Self::Dashboard => "dashboard",
            Self::Drain => "drain",
            Self::Clock => "clock tick",
        })
    }
}

/// One live participant, as its own record describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParticipantRecord {
    pub pid: u32,
    pub role: ParticipantRole,
    /// When this process image joined the authority. Participants join
    /// before bootstrap, so this is the start of the running image.
    pub started_at: DateTime<Utc>,
    pub access: Access,
    /// SHA-256 of the executable image.
    pub digest: String,
    pub identity: CompatibilityIdentity,
    /// The resume capability this process hands over with once a candidate
    /// is renamed over its executable; `None` when it cannot hand over.
    /// Builds that predate it read and write records without the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handover: Option<String>,
}

impl fmt::Display for ParticipantRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pid {} ({}, started {})",
            self.pid,
            self.role,
            self.started_at.to_rfc3339_opts(SecondsFormat::Secs, true)
        )
    }
}

/// This process's registration. Dropping it withdraws the record.
pub(super) struct Registration {
    file: File,
    path: PathBuf,
}

impl Drop for Registration {
    fn drop(&mut self) {
        // Unlock before unlinking, so a forked-but-not-exec'd child holding
        // the same description cannot keep a withdrawn record alive.
        let _ = FileExt::unlock(&self.file);
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Register `record` under `root`. Call with admission held, shared or
/// exclusive.
///
/// `None` means this process cannot write under the root (a read-only mount,
/// a sandboxed child): it still participates through the generation lock,
/// but cannot be named as a blocker or asked to yield.
/// Records left by processes that exited without withdrawing them (a
/// `process::exit`, a kill) are removed first.
pub(super) fn register(root: &Path, record: &ParticipantRecord) -> Option<Registration> {
    let root = validated_generation_root(root).ok()?;
    let dir = root.join(PARTICIPANTS_DIR);
    // Containment is checked beside each sink in this file, as `open` does in
    // the parent module: code scanning does not credit a check made inside a
    // helper that returns the path.
    if !dir.starts_with(&root) {
        return None;
    }
    std::fs::create_dir_all(&dir).ok()?;
    let _ = live_participants(&root, None, true);
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).ok()?;
    let name = format!(
        "{}-{}.json",
        record.pid,
        nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    let path = dir.join(&name);
    let staged = dir.join(format!("{name}.{STAGED_EXTENSION}"));
    if !path.starts_with(&dir) || !staged.starts_with(&dir) {
        return None;
    }
    // Concurrent joiners collect records while this one registers, so the
    // record is created, locked and written under a name they skip, and only
    // then renamed into place: no collector sees it unlocked.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&staged)
        .ok()?;
    let registered = (|| {
        FileExt::try_lock_exclusive(&file).ok()?;
        let encoded = serde_json::to_vec(record).ok()?;
        file.write_all(&encoded).ok()?;
        file.sync_data().ok()?;
        std::fs::rename(&staged, &path).ok()?;
        Some(Registration {
            file: file.try_clone().ok()?,
            path: path.clone(),
        })
    })();
    if registered.is_none() {
        let _ = std::fs::remove_file(&staged);
        let _ = std::fs::remove_file(&path);
    }
    registered
}

/// Every live participant under `root` other than `except`. With `collect`,
/// records whose owner has exited are removed, as are staged records a
/// crashed registration left behind.
pub(super) fn live_participants(
    root: &Path,
    except: Option<&Registration>,
    collect: bool,
) -> Vec<ParticipantRecord> {
    let Ok(root) = validated_generation_root(root) else {
        return Vec::new();
    };
    let dir = root.join(PARTICIPANTS_DIR);
    if !dir.starts_with(&root) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut live = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.starts_with(&dir) {
            continue;
        }
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("json") => {}
            Some(STAGED_EXTENSION) if collect => {
                collect_abandoned_stage(&path);
                continue;
            }
            _ => continue,
        }
        if except.is_some_and(|own| own.path == path) {
            continue;
        }
        let Ok(mut file) = File::open(&path) else {
            continue;
        };
        if FileExt::try_lock_shared(&file).is_ok() {
            // Nobody holds it: the owner exited without withdrawing it.
            let _ = FileExt::unlock(&file);
            if collect {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        if let Some(record) = read_record::<ParticipantRecord>(&mut file) {
            live.push(record);
        }
    }
    live.sort_by_key(|record| (record.started_at, record.pid));
    live
}

/// Remove a staged record nobody holds that is too old to be one a live
/// registration created and has yet to lock.
pub(super) fn collect_abandoned_stage(path: &Path) {
    let abandoned = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age > STAGED_GRACE);
    if !abandoned {
        return;
    }
    let Ok(file) = File::open(path) else {
        return;
    };
    if FileExt::try_lock_shared(&file).is_ok() {
        let _ = FileExt::unlock(&file);
        let _ = std::fs::remove_file(path);
    }
}

fn read_record<T: for<'de> Deserialize<'de>>(file: &mut File) -> Option<T> {
    if file.metadata().ok()?.len() > MAX_RECORD_BYTES {
        return None;
    }
    let mut raw = String::new();
    file.seek(SeekFrom::Start(0)).ok()?;
    file.read_to_string(&mut raw).ok()?;
    serde_json::from_str(&raw).ok()
}

/// A generation switch waiting for the live participants to yield.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSwitch {
    /// The waiting process.
    pub pid: u32,
    pub role: ParticipantRole,
    /// Executable digest of the waiting binary.
    pub digest: String,
    /// The identity it will record once admitted.
    pub target: CompatibilityIdentity,
    pub requested_at: DateTime<Utc>,
    /// When the waiter gives up and refuses.
    pub deadline: DateTime<Utc>,
}

/// The waiter's hold on the pending record. Dropping it clears the switch.
pub(super) struct PendingClaim {
    file: File,
}

impl Drop for PendingClaim {
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
        let _ = self.file.sync_data();
        let _ = FileExt::unlock(&self.file);
    }
}

/// Record `switch` as pending. Call with exclusive admission held, after
/// [`pending_switch`] found none.
pub(super) fn claim_pending(
    root: &Path,
    switch: &PendingSwitch,
) -> Result<PendingClaim, OrbitError> {
    let root = validated_generation_root(root)?;
    let path = root.join(PENDING_RECORD);
    if !path.starts_with(&root) {
        return Err(refusal("pending record path escapes the root"));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| {
            refusal(format!(
                "cannot record the pending generation switch: {error}"
            ))
        })?;
    // A `pending_switch` reader holds the record shared for an instant; only a
    // claim that outlasts a short retry is another waiter's.
    let settle = std::time::Instant::now() + CLAIM_SETTLE;
    while FileExt::try_lock_exclusive(&file).is_err() {
        if std::time::Instant::now() >= settle {
            return Err(refusal("another generation switch is already pending"));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let claim = PendingClaim {
        file: file.try_clone().map_err(refusal)?,
    };
    let encoded = serde_json::to_vec(switch).map_err(refusal)?;
    file.seek(SeekFrom::Start(0)).map_err(refusal)?;
    file.write_all(&encoded).map_err(refusal)?;
    file.set_len(encoded.len() as u64).map_err(refusal)?;
    file.sync_data().map_err(refusal)?;
    Ok(claim)
}

/// The pending switch under `root`, if a live waiter holds one.
pub fn pending_switch(root: &Path) -> Option<PendingSwitch> {
    let root = validated_generation_root(root).ok()?;
    let path = root.join(PENDING_RECORD);
    if !path.starts_with(&root) {
        return None;
    }
    let mut file = File::open(&path).ok()?;
    if FileExt::try_lock_shared(&file).is_ok() {
        let _ = FileExt::unlock(&file);
        return None;
    }
    read_record(&mut file)
}
