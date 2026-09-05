//! Staging, verification, and replacement of the installed executable.
//!
//! Nothing here touches the live executable until a complete, verified
//! replacement is already sitting next to it: the archive is downloaded,
//! its manifest signature checked, its digest compared, and its single
//! member extracted to a sibling staging file. The swap is then one
//! same-directory rename, which is atomic — so a crash can leave a stray
//! staging file, but never a half-written `orbit`.

use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use orbit_common::OrbitError;
use orbit_common::security::release::{
    RELEASE_CHECKSUMS_FILENAME, RELEASE_CHECKSUMS_SIGNATURE_FILENAME, TrustedReleaseKey,
    checksum_for_asset, sha256_hex, verify_checksum_signature, verify_sha256_digest,
};

use super::source::ReleaseSource;

/// Name of the archive member every Orbit release archive must contain, and
/// nothing else.
const ARCHIVE_MEMBER: &str = "orbit";

/// Largest release archive we will buffer. A published `orbit` archive is a
/// few tens of megabytes; anything past this is a redirect to the wrong thing.
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;

/// A verified replacement executable staged beside its destination.
#[derive(Debug)]
pub struct StagedRelease {
    /// The staging file, removed on drop unless it was committed.
    path: PathBuf,
    /// SHA-256 of the release archive this came from.
    pub archive_sha256: String,
    /// ID of the release signing key that authenticated the manifest.
    pub signing_key_id: String,
    committed: bool,
}

impl StagedRelease {
    /// Path of the staged executable, runnable for pre-flight checks.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Replace `destination` with the staged executable, first copying the
    /// current one aside to `backup`.
    ///
    /// The backup is a copy rather than a rename so the destination is never
    /// momentarily absent: a concurrent `orbit` launch either sees the old
    /// binary or the new one.
    pub fn commit(mut self, destination: &Path, backup: &Path) -> Result<(), OrbitError> {
        std::fs::copy(destination, backup).map_err(|error| {
            OrbitError::Io(format!(
                "failed to back up '{}' to '{}': {error}",
                destination.display(),
                backup.display()
            ))
        })?;
        std::fs::rename(&self.path, destination).map_err(|error| {
            OrbitError::Io(format!(
                "failed to install the staged release over '{}': {error}",
                destination.display()
            ))
        })?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for StagedRelease {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Restore `destination` from `backup` after a failed replacement.
///
/// Only safe while nothing has run the replacement binary against durable
/// state; once migrations have been attempted the caller must resume forward
/// instead.
pub fn restore_backup(destination: &Path, backup: &Path) -> Result<(), OrbitError> {
    std::fs::copy(backup, destination)
        .map(|_| ())
        .map_err(|error| {
            OrbitError::Io(format!(
                "failed to restore '{}' from '{}': {error}; the backup is still on disk",
                destination.display(),
                backup.display()
            ))
        })
}

/// Download `asset` for `version`, authenticate it, and stage the executable
/// it contains next to `destination`.
pub fn stage_release(
    source: &dyn ReleaseSource,
    version: &str,
    asset: &str,
    destination: &Path,
    trusted_keys: &'static [TrustedReleaseKey],
    today: NaiveDate,
) -> Result<StagedRelease, OrbitError> {
    let manifest_bytes = source.fetch(version, RELEASE_CHECKSUMS_FILENAME)?;
    let signature = source.fetch(version, RELEASE_CHECKSUMS_SIGNATURE_FILENAME)?;
    let signing_key_id =
        verify_checksum_signature(&manifest_bytes, &signature, trusted_keys, today)?;
    let manifest = std::str::from_utf8(&manifest_bytes).map_err(|error| {
        OrbitError::Execution(format!(
            "{RELEASE_CHECKSUMS_FILENAME} is not UTF-8: {error}"
        ))
    })?;
    let expected = checksum_for_asset(manifest, asset)?;

    let archive = source.fetch(version, asset)?;
    let archive_sha256 = sha256_hex(&archive);
    verify_sha256_digest(&archive_sha256, &expected, asset)?;

    let executable = extract_release_executable(&archive, asset)?;
    let path = write_staging_file(destination, &executable)?;
    Ok(StagedRelease {
        path,
        archive_sha256,
        signing_key_id: signing_key_id.to_string(),
        committed: false,
    })
}

/// Extract the single `orbit` member from a release archive.
///
/// Mirrors the archive checks in `install.sh`: exactly one member, named
/// `orbit`, a regular file, with no path separator or traversal in its name.
/// A release archive is authenticated by this point, so these are defense in
/// depth against a signing-side mistake rather than the primary control.
fn extract_release_executable(archive: &[u8], asset: &str) -> Result<Vec<u8>, OrbitError> {
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut reader = tar::Archive::new(decoder);
    let entries = reader
        .entries()
        .map_err(|error| OrbitError::Execution(format!("could not read {asset}: {error}")))?;

    let mut executable = None;
    for entry in entries {
        let entry = entry.map_err(|error| {
            OrbitError::Execution(format!("could not read a member of {asset}: {error}"))
        })?;
        let path = entry
            .path()
            .map_err(|error| {
                OrbitError::Execution(format!("{asset} has an unreadable member name: {error}"))
            })?
            .into_owned();
        let name = path.to_string_lossy().into_owned();
        if executable.is_some() {
            return Err(OrbitError::Execution(format!(
                "{asset} must contain only `{ARCHIVE_MEMBER}`, but also contains `{name}`"
            )));
        }
        if name != ARCHIVE_MEMBER {
            return Err(OrbitError::Execution(format!(
                "unexpected member `{name}` in {asset}; expected only `{ARCHIVE_MEMBER}`"
            )));
        }
        if !entry.header().entry_type().is_file() {
            return Err(OrbitError::Execution(format!(
                "`{ARCHIVE_MEMBER}` in {asset} is not a regular file"
            )));
        }
        let declared = entry.header().size().map_err(|error| {
            OrbitError::Execution(format!("{asset} has an unreadable member size: {error}"))
        })?;
        if declared > MAX_ARCHIVE_BYTES {
            return Err(OrbitError::Execution(format!(
                "`{ARCHIVE_MEMBER}` in {asset} declares {declared} bytes, past the {MAX_ARCHIVE_BYTES}-byte ceiling"
            )));
        }
        let mut bytes = Vec::with_capacity(declared as usize);
        entry
            .take(MAX_ARCHIVE_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                OrbitError::Execution(format!("could not extract `{ARCHIVE_MEMBER}`: {error}"))
            })?;
        executable = Some(bytes);
    }

    executable.ok_or_else(|| {
        OrbitError::Execution(format!("{asset} does not contain `{ARCHIVE_MEMBER}`"))
    })
}

/// Write the staged executable beside `destination` so the swap is a
/// same-directory rename.
fn write_staging_file(destination: &Path, executable: &[u8]) -> Result<PathBuf, OrbitError> {
    let directory = destination.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "'{}' has no parent directory to stage into",
            destination.display()
        ))
    })?;
    let path = directory.join(format!(".orbit-update-staged.{}", std::process::id()));
    std::fs::write(&path, executable).map_err(|error| {
        OrbitError::Io(format!(
            "failed to stage the replacement executable at '{}': {error}",
            path.display()
        ))
    })?;
    set_executable_mode(&path)?;
    Ok(path)
}

#[cfg(unix)]
fn set_executable_mode(path: &Path) -> Result<(), OrbitError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|error| {
        OrbitError::Io(format!(
            "failed to make '{}' executable: {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_executable_mode(_path: &Path) -> Result<(), OrbitError> {
    Ok(())
}
