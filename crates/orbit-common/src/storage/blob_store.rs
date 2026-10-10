//! Content-addressed blob store.
//!
//! Writes bytes to `{root}/{hash[..2]}/{hash}` keyed by sha256 of the
//! post-redaction content. De-duplicates: if the target path already exists
//! the write is a no-op. Intended for audit payload storage where
//! events reference blobs by hash rather than path.
//!
//! Redaction runs at write time via [`redact_all`] plus an optional
//! caller-supplied [`PatternRedactor`]; the stored bytes are already safe, so
//! read-side tooling does not need to re-apply it. Blob hashes are computed
//! from those post-redaction bytes.
//!
//! A store with a pending-publication root ([`BlobStore::with_pending_root`])
//! takes part in the audit blob sweep's protocol ([`super::blob_sweep`]): each
//! write records a pending marker before the blob is published, and a write
//! of content already stored refreshes the blob's mtime, so the sweep never
//! removes a blob between its write and the row that names it.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::blob_sweep;
use crate::fs::io::{atomic_write_private_bytes, create_private_dir_all};
use crate::security::redaction::{PatternRedactor, redact_all};
use crate::security::release::sha256_hex;

pub struct BlobStore {
    root: PathBuf,
    extra_redactor: PatternRedactor,
    pending_root: Option<PathBuf>,
}

impl BlobStore {
    pub fn new<P: Into<PathBuf>>(root: P) -> Self {
        Self {
            root: root.into(),
            extra_redactor: PatternRedactor::empty(),
            pending_root: None,
        }
    }

    /// Record a pending-publication marker under `pending_root` for every
    /// write, until [`BlobStore::clear_published`] retires it.
    pub fn with_pending_root(mut self, pending_root: impl Into<PathBuf>) -> Self {
        self.pending_root = Some(pending_root.into());
        self
    }

    /// Add caller-specific pattern redaction on top of the mandatory
    /// `redact_all()` pass. This cannot weaken the default env-value and HTTP
    /// pattern redaction applied by [`BlobStore::write`].
    pub fn with_redaction(mut self, redactor: PatternRedactor) -> Self {
        self.extra_redactor = redactor;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn write(&self, content: &[u8]) -> io::Result<String> {
        let redacted = self.redact_for_storage(content);
        let hash = sha256_hex(&redacted);
        // The marker precedes the blob: the sweep rechecks it after moving a
        // blob aside, so it either sees this marker or this write sees the
        // blob gone and stores it again.
        if let Some(pending_root) = &self.pending_root {
            blob_sweep::mark_pending(pending_root, &hash)?;
        }
        let dir = self.root.join(&hash[..2]);
        create_private_dir_all(&dir)?;
        let path = dir.join(&hash);
        if !refresh_existing(&path, &hash)? {
            atomic_write_private_bytes(&path, &redacted)?;
        }
        Ok(hash)
    }

    /// Retire the pending markers of every blob `published` names, once the
    /// row holding that text is durable. Best effort: a marker left behind is
    /// only swept late, never early.
    pub fn clear_published(&self, published: &str) {
        if let Some(pending_root) = &self.pending_root {
            blob_sweep::clear_pending(pending_root, published);
        }
    }

    /// Return the bytes that would be persisted for `content` after the
    /// mandatory env/pattern redaction and any extra caller redactor.
    pub fn redact_for_storage(&self, content: &[u8]) -> Vec<u8> {
        match std::str::from_utf8(content) {
            Ok(text) => self
                .extra_redactor
                .apply_str(&redact_all(text))
                .into_bytes(),
            Err(_) => {
                let lossy_text = String::from_utf8_lossy(content);
                self.extra_redactor
                    .apply_str(&redact_all(&lossy_text))
                    .into_bytes()
            }
        }
    }

    pub fn read(&self, sha256: &str) -> io::Result<Vec<u8>> {
        fs::read(self.blob_path(sha256)?)
    }

    /// Read at most `max_bytes` from the blob, without loading the rest of
    /// the file. Callers that only need a preview window should use this
    /// instead of [`Self::read`].
    pub fn read_prefix(&self, sha256: &str, max_bytes: usize) -> io::Result<Vec<u8>> {
        let file = fs::File::open(self.blob_path(sha256)?)?;
        let mut buf = Vec::new();
        file.take(max_bytes as u64).read_to_end(&mut buf)?;
        Ok(buf)
    }

    /// `{root}/{hash[..2]}/{hash}` for a stored reference.
    ///
    /// References come back from persisted audit rows, so anything other than
    /// a lowercase SHA-256 digest is refused rather than sliced mid-character
    /// or joined into a path outside the store.
    fn blob_path(&self, sha256: &str) -> io::Result<PathBuf> {
        let is_digest = sha256.len() == 64
            && sha256
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !is_digest {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid blob reference '{sha256}'"),
            ));
        }
        Ok(self.root.join(&sha256[..2]).join(sha256))
    }
}

/// Whether `path` already holds `hash`'s content, refreshing its mtime when it
/// does: content-addressed writes share one file, and the sweep keeps a blob
/// touched inside its grace window.
fn refresh_existing(path: &Path, hash: &str) -> io::Result<bool> {
    if !path_matches_hash(path, hash)? {
        return Ok(false);
    }
    match fs::OpenOptions::new().append(true).open(path) {
        Ok(file) => {
            file.set_modified(SystemTime::now())?;
            Ok(true)
        }
        // The sweep moved it aside after the read: store it again.
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn path_matches_hash(path: &Path, expected_hash: &str) -> io::Result<bool> {
    match fs::read(path) {
        Ok(bytes) => Ok(sha256_hex(&bytes) == expected_hash),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
