//! The admission files: opening them, and the v1 digest and v2 compatibility records.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::GENERATION_CONTRACT;
use super::identity::Envelope;
use super::paths::{COMPAT_RECORD, validated_generation_record_path, validated_generation_root};
use super::refusal::{refusal, unwritable};
use crate::OrbitError;

/// An admission file and whether this process may rewrite its bytes.
///
/// A participant can join an existing generation from a read-only mount, or
/// from a sandboxed child denied writes under the authority root: flock needs
/// a descriptor, not permission to rewrite bytes. Such a descriptor can never
/// record a takeover, so writability travels with the open instead of
/// surfacing as `write` failing with `EBADF` half way through the protocol.
pub(super) struct Record {
    pub(super) file: File,
    pub(super) writable: bool,
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

pub(super) fn open(root: &Path, name: &str) -> Result<Record, OrbitError> {
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

pub(super) fn read_generation(file: &mut File) -> Result<String, OrbitError> {
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

pub(super) fn read_compat(root: &Path, recorded: &str) -> Option<Envelope> {
    if recorded.is_empty() {
        return None;
    }
    let root = validated_generation_root(root).ok()?;
    let path = validated_generation_record_path(&root, COMPAT_RECORD).ok()?;
    // Re-checked beside the open, as `open` does: code scanning does not
    // credit the containment check inside the helper that built the path.
    if !path.starts_with(&root) {
        return None;
    }
    let file = File::open(&path).ok()?;
    let mut raw = String::new();
    file.take(16 * 1024).read_to_string(&mut raw).ok()?;
    let record: CompatRecord = serde_json::from_str(&raw).ok()?;
    (record.contract == GENERATION_CONTRACT && record.record_digest == recorded)
        .then_some(record.envelope)
}

pub(super) fn write_compat(
    root: &Path,
    digest: &str,
    envelope: &Envelope,
) -> Result<(), OrbitError> {
    let path = validated_generation_record_path(root, COMPAT_RECORD)?;
    let encoded = serde_json::to_string(&CompatRecord {
        contract: GENERATION_CONTRACT.to_string(),
        record_digest: digest.to_string(),
        envelope: envelope.clone(),
    })
    .map_err(refusal)?;
    crate::fs::io::atomic_write_text(&path, &encoded).map_err(unwritable)
}
