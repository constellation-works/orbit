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

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::fs::io::{atomic_write_private_bytes, create_private_dir_all};
use crate::security::redaction::{PatternRedactor, redact_all};

pub struct BlobStore {
    root: PathBuf,
    extra_redactor: PatternRedactor,
}

impl BlobStore {
    pub fn new<P: Into<PathBuf>>(root: P) -> Self {
        Self {
            root: root.into(),
            extra_redactor: PatternRedactor::empty(),
        }
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
        let dir = self.root.join(&hash[..2]);
        create_private_dir_all(&dir)?;
        let path = dir.join(&hash);
        if !path_matches_hash(&path, &hash)? {
            atomic_write_private_bytes(&path, &redacted)?;
        }
        Ok(hash)
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
        let path = self.root.join(&sha256[..2]).join(sha256);
        fs::read(path)
    }
}

fn path_matches_hash(path: &Path, expected_hash: &str) -> io::Result<bool> {
    match fs::read(path) {
        Ok(bytes) => Ok(sha256_hex(&bytes) == expected_hash),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}
