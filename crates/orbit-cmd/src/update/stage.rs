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

use super::source::{MAX_ARCHIVE_BYTES, ReleaseSource};

/// Name of the archive member every Orbit release archive must contain, and
/// nothing else.
const ARCHIVE_MEMBER: &str = "orbit";

/// A verified replacement executable staged beside its destination.
#[derive(Debug)]
pub struct StagedRelease {
    executable: StagedExecutable,
    /// SHA-256 of the release archive this came from.
    pub archive_sha256: String,
    /// ID of the release signing key that authenticated the manifest.
    pub signing_key_id: String,
}

impl StagedRelease {
    /// Path of the staged executable, runnable for pre-flight checks.
    pub fn path(&self) -> &Path {
        self.executable.path()
    }

    /// Replace `destination` with the staged executable; see
    /// [`StagedExecutable::commit`].
    pub fn commit(self, destination: &Path, backup: &Path) -> Result<(), OrbitError> {
        self.executable.commit(destination, backup)
    }
}

/// An executable written beside its destination so the swap is one
/// same-directory rename.
#[derive(Debug)]
pub struct StagedExecutable {
    /// The staging file, removed on drop unless it was committed.
    path: PathBuf,
    committed: bool,
}

impl StagedExecutable {
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

impl Drop for StagedExecutable {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Stream at most `limit` bytes of `executable` into a staging file beside
/// `destination`, without installing it.
pub(super) fn stage_executable(
    destination: &Path,
    executable: impl Read,
    limit: u64,
) -> Result<StagedExecutable, OrbitError> {
    Ok(StagedExecutable {
        path: write_staging_file(destination, executable, limit)?,
        committed: false,
    })
}

/// Restore `destination` from `backup` after a failed replacement.
///
/// Only safe while nothing has run the replacement binary against durable
/// state; once migrations have been attempted the caller must resume forward
/// instead.
pub fn restore_backup(destination: &Path, backup: &Path) -> Result<(), OrbitError> {
    restore_backup_with_rename(destination, backup, |from, to| std::fs::rename(from, to))
}

/// Reject an installed candidate without losing its verification failure if
/// restoring the previous executable also fails. No workspace state has run yet.
pub(super) fn reject_installed_candidate(
    destination: &Path,
    backup: &Path,
    verification_failure: String,
    restore: impl FnOnce(&Path, &Path) -> Result<(), OrbitError>,
) -> OrbitError {
    match restore(destination, backup) {
        Ok(()) => OrbitError::Execution(format!(
            "{verification_failure}; restored the previous executable and changed no workspace state"
        )),
        Err(restore_error) => OrbitError::Execution(format!(
            "{verification_failure}; restoring the previous executable also failed ({restore_error}); \
             the rejected candidate remains installed at '{}', and the previous executable's \
             backup remains at '{}'; no workspace state was changed. Restore the backup before \
             retrying the update",
            destination.display(),
            backup.display()
        )),
    }
}

/// Stage a complete copy of `backup`, then atomically replace `destination`.
///
/// Keeping the replace operation injectable lets the sibling tests exercise
/// the recovery evidence and cleanup path without depending on filesystem
/// permission behavior.
pub(super) fn restore_backup_with_rename(
    destination: &Path,
    backup: &Path,
    replace: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), OrbitError> {
    let staging = sibling_staging_path(destination, ".orbit-update-restore")?;
    if let Err(error) = std::fs::copy(backup, &staging) {
        return Err(restore_failure(
            destination,
            backup,
            &staging,
            "stage the previous executable",
            error,
        ));
    }
    if let Err(error) = replace(&staging, destination) {
        return Err(restore_failure(
            destination,
            backup,
            &staging,
            "atomically replace the installed executable",
            error,
        ));
    }
    Ok(())
}

fn restore_failure(
    destination: &Path,
    backup: &Path,
    staging: &Path,
    action: &str,
    error: std::io::Error,
) -> OrbitError {
    let cleanup = match std::fs::remove_file(staging) {
        Ok(()) => "the incomplete restore staging file was removed".to_string(),
        Err(cleanup_error) if cleanup_error.kind() == std::io::ErrorKind::NotFound => {
            "no restore staging file was left behind".to_string()
        }
        Err(cleanup_error) => format!(
            "cleanup also failed for restore staging file '{}': {cleanup_error}; remove it before retrying",
            staging.display()
        ),
    };
    OrbitError::Io(format!(
        "failed to {action} at '{}': {error}; the installed executable at '{}' was left intact, the backup remains at '{}', and {cleanup}",
        staging.display(),
        destination.display(),
        backup.display()
    ))
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
    Ok(StagedRelease {
        executable: stage_executable(destination, executable.as_slice(), MAX_ARCHIVE_BYTES)?,
        archive_sha256,
        signing_key_id: signing_key_id.to_string(),
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
///
/// The file is created fresh (`create_new` never follows a pre-planted
/// symlink at the predictable name) and synced before the rename, so a crash
/// cannot swap in a truncated binary. More than `limit` bytes is refused, and
/// the partial file removed. A mode-setting failure also removes the file.
fn write_staging_file(
    destination: &Path,
    executable: impl Read,
    limit: u64,
) -> Result<PathBuf, OrbitError> {
    write_staging_file_with_mode(destination, executable, limit, set_executable_mode)
}

/// Inject the mode operation to exercise staging cleanup without relying on
/// filesystem-specific permission failures.
pub(super) fn write_staging_file_with_mode(
    destination: &Path,
    executable: impl Read,
    limit: u64,
    set_mode: impl FnOnce(&Path) -> Result<(), OrbitError>,
) -> Result<PathBuf, OrbitError> {
    let path = sibling_staging_path(destination, ".orbit-update-staged")?;
    let stage_error = |error: std::io::Error| {
        OrbitError::Io(format!(
            "failed to stage the replacement executable at '{}': {error}",
            path.display()
        ))
    };
    // A leftover from a crashed update under a reused PID; removing a symlink
    // removes the link, never its target.
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(stage_error(error)),
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(stage_error)?;
    let written = std::io::copy(&mut executable.take(limit + 1), &mut file)
        .and_then(|written| file.sync_all().map(|()| written))
        .map_err(|error| {
            let _ = std::fs::remove_file(&path);
            stage_error(error)
        })?;
    if written > limit {
        let _ = std::fs::remove_file(&path);
        return Err(OrbitError::Execution(format!(
            "the replacement executable exceeds the {limit}-byte staging limit"
        )));
    }
    drop(file);
    if let Err(error) = set_mode(&path) {
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

fn sibling_staging_path(destination: &Path, prefix: &str) -> Result<PathBuf, OrbitError> {
    let directory = destination.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "'{}' has no parent directory to stage into",
            destination.display()
        ))
    })?;
    Ok(directory.join(format!("{prefix}.{}", std::process::id())))
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
