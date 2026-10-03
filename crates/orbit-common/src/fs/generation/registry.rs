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

use chrono::{DateTime, SecondsFormat, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::identity::{Access, CompatibilityIdentity};
use super::paths::validated_generation_root;
use super::refusal::refusal;
use crate::OrbitError;

const PARTICIPANTS_DIR: &str = ".generation-participants";
const PENDING_RECORD: &str = ".generation-pending.json";
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
}

impl fmt::Display for ParticipantRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Command => "command",
            Self::McpServe => "mcp serve",
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

/// Register `record` under `root`. Call with the admission lock held.
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
    let path = dir.join(name);
    if !path.starts_with(&dir) {
        return None;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .ok()?;
    let registration = Registration {
        file: file.try_clone().ok()?,
        path,
    };
    FileExt::try_lock_exclusive(&file).ok()?;
    let encoded = serde_json::to_vec(record).ok()?;
    file.write_all(&encoded).ok()?;
    file.sync_data().ok()?;
    Some(registration)
}

/// Every live participant under `root` other than `except`. With `collect`,
/// records whose owner has exited are removed; pass it only while holding the
/// admission lock, so no registration is between create and lock.
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
        if !path.starts_with(&dir) || path.extension().and_then(|ext| ext.to_str()) != Some("json")
        {
            continue;
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

/// Record `switch` as pending. Call with the admission lock held, after
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
    FileExt::try_lock_exclusive(&file)
        .map_err(|_| refusal("another generation switch is already pending"))?;
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
