//! Remembers the SHA-256 of an executable image between processes.
//!
//! Every Orbit command joins the generation by its own executable digest, and
//! hashing a multi-megabyte binary costs more than the rest of a short
//! command. The digest of an image never changes while its file identity does
//! not, so the answer is cached beside the generation records, keyed by what
//! the kernel says identifies the bytes: device, inode, size, mtime and ctime.
//! A rewrite or replacement changes at least one of them (ctime cannot be set
//! by userspace), so the next process hashes again.
//!
//! Like git's racy-clean rule, an entry is never written for an image whose
//! ctime is within [`SETTLE`] of now: a second rewrite inside the same
//! timestamp tick could otherwise leave an identical key over different bytes.
//! The cache is best-effort; any read, parse or write failure hashes as
//! before.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::OrbitError;

/// An image younger than this is hashed every time and never cached.
const SETTLE: Duration = Duration::from_secs(2);
/// Distinct images remembered (installed binary, staged candidate, checkout
/// builds); the oldest entries fall off.
const MAX_ENTRIES: usize = 8;
/// The record is small JSON; anything larger is not ours.
const MAX_CACHE_BYTES: u64 = 16 * 1024;
const VERSION: u32 = 1;

/// The kernel's identity for one version of one file's bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ImageKey {
    dev: u64,
    ino: u64,
    size: u64,
    mtime_sec: i64,
    mtime_nsec: i64,
    ctime_sec: i64,
    ctime_nsec: i64,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    #[serde(flatten)]
    key: ImageKey,
    sha256: String,
}

#[derive(Serialize, Deserialize)]
struct Cache {
    version: u32,
    entries: Vec<Entry>,
}

#[cfg(unix)]
fn key_of(file: &File) -> Option<ImageKey> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata().ok()?;
    Some(ImageKey {
        dev: meta.dev(),
        ino: meta.ino(),
        size: meta.size(),
        mtime_sec: meta.mtime(),
        mtime_nsec: meta.mtime_nsec(),
        ctime_sec: meta.ctime(),
        ctime_nsec: meta.ctime_nsec(),
    })
}

#[cfg(not(unix))]
fn key_of(_file: &File) -> Option<ImageKey> {
    None
}

fn is_sha256_hex(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
}

fn load(path: &Path) -> Option<Cache> {
    let mut raw = String::new();
    File::open(path)
        .ok()?
        .take(MAX_CACHE_BYTES)
        .read_to_string(&mut raw)
        .ok()?;
    let cache: Cache = serde_json::from_str(&raw).ok()?;
    (cache.version == VERSION).then_some(cache)
}

/// Whether the image has been stable for long enough that an equal key can
/// only mean equal bytes.
fn settled(key: &ImageKey, now: SystemTime) -> bool {
    let Ok(ctime_sec) = u64::try_from(key.ctime_sec) else {
        return false;
    };
    let changed =
        UNIX_EPOCH + Duration::new(ctime_sec, key.ctime_nsec.clamp(0, 999_999_999) as u32);
    now.duration_since(changed).is_ok_and(|age| age >= SETTLE)
}

fn store(path: &Path, key: ImageKey, digest: &str, now: SystemTime) {
    if !settled(&key, now) || !path.parent().is_some_and(Path::is_dir) {
        return;
    }
    let mut entries = load(path).map(|cache| cache.entries).unwrap_or_default();
    entries.retain(|entry| entry.key.dev != key.dev || entry.key.ino != key.ino);
    entries.insert(
        0,
        Entry {
            key,
            sha256: digest.to_string(),
        },
    );
    entries.truncate(MAX_ENTRIES);
    let cache = Cache {
        version: VERSION,
        entries,
    };
    if let Ok(encoded) = serde_json::to_string(&cache) {
        // Losing the rename to a crash only costs one more hash.
        let _ = crate::fs::io::atomic_write_text_volatile(path, &encoded);
    }
}

/// The digest of `file`, from the cache at `cache_path` when the image's file
/// identity matches an entry, otherwise from `hash` (remembered afterwards).
pub(super) fn digest_with_cache(
    cache_path: Option<&Path>,
    file: File,
    hash: impl FnOnce(File) -> Result<String, OrbitError>,
) -> Result<String, OrbitError> {
    let (Some(path), Some(key)) = (cache_path, key_of(&file)) else {
        return hash(file);
    };
    if let Some(entry) = load(path).and_then(|cache| {
        cache
            .entries
            .into_iter()
            .find(|entry| entry.key == key && is_sha256_hex(&entry.sha256))
    }) {
        return Ok(entry.sha256);
    }
    let digest = hash(file)?;
    store(path, key, &digest, SystemTime::now());
    Ok(digest)
}

#[cfg(test)]
#[path = "tests/image_digest.rs"]
mod tests;
