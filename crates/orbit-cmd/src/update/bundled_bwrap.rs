//! The bundled Bubblewrap published with every Linux release.
//!
//! `orbit init` installs it only when the host's own `bwrap` is missing or
//! lacks `--bind-fd`. It is authenticated exactly like an `orbit update`
//! archive before anything privileged runs: the release's checksum manifest
//! must carry a trusted signature, and the binary must hash to the digest that
//! manifest lists for it. An unsigned, unlisted, or mismatched binary is
//! refused. The binary always comes from the release of the running Orbit, so
//! upgrading Orbit is what moves the bundled Bubblewrap forward.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use orbit_common::OrbitError;
use orbit_common::security::release::{
    RELEASE_CHECKSUMS_FILENAME, RELEASE_CHECKSUMS_SIGNATURE_FILENAME, TrustedReleaseKey,
    checksum_for_asset, sha256_hex, verify_checksum_signature, verify_sha256_digest,
};

use super::source::{ReleaseSource, release_source_from_env};

/// Release asset holding the static Bubblewrap for `arch`
/// (`std::env::consts::ARCH`), or `None` where none is published.
pub fn bundled_bwrap_asset_name(arch: &str) -> Option<String> {
    matches!(arch, "x86_64" | "aarch64").then(|| format!("orbit-bwrap-{arch}-linux"))
}

/// An authenticated bundled Bubblewrap in a private directory, removed on drop.
#[derive(Debug)]
pub struct StagedBwrap {
    _directory: tempfile::TempDir,
    path: PathBuf,
    /// SHA-256 the signed manifest lists, which the staged bytes match.
    pub sha256: String,
    /// Release signing key that authenticated the manifest.
    pub signing_key_id: String,
}

impl StagedBwrap {
    /// The staged executable.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Fetch and authenticate `asset` as published with `version`.
pub fn stage_bundled_bwrap(
    source: &dyn ReleaseSource,
    version: &str,
    asset: &str,
    trusted_keys: &'static [TrustedReleaseKey],
    today: NaiveDate,
) -> Result<StagedBwrap, OrbitError> {
    let manifest_bytes = source.fetch(version, RELEASE_CHECKSUMS_FILENAME)?;
    let signature = source.fetch(version, RELEASE_CHECKSUMS_SIGNATURE_FILENAME)?;
    let signing_key_id =
        verify_checksum_signature(&manifest_bytes, &signature, trusted_keys, today)?;
    let manifest = std::str::from_utf8(&manifest_bytes).map_err(|error| {
        OrbitError::Execution(format!(
            "{RELEASE_CHECKSUMS_FILENAME} is not UTF-8: {error}"
        ))
    })?;
    let expected = checksum_for_asset(manifest, asset).map_err(|error| {
        OrbitError::Execution(format!(
            "Orbit {version} publishes no signed bundled Bubblewrap for this host: {error}"
        ))
    })?;
    let binary = source.fetch(version, asset)?;
    let sha256 = sha256_hex(&binary);
    verify_sha256_digest(&sha256, &expected, asset)?;

    let directory = tempfile::Builder::new()
        .prefix("orbit-bwrap-")
        .tempdir()
        .map_err(|error| {
            OrbitError::Io(format!(
                "failed to create a private staging directory: {error}"
            ))
        })?;
    let path = directory.path().join("bwrap");
    write_executable(&path, &binary)?;
    Ok(StagedBwrap {
        _directory: directory,
        path,
        sha256,
        signing_key_id: signing_key_id.to_string(),
    })
}

/// [`stage_bundled_bwrap`] for this machine's architecture from the release of
/// the running Orbit, read from the same source and trust set `orbit update`
/// uses (`ORBIT_UPDATE_RELEASE_DIR`, `ORBIT_INSTALL_REPO`,
/// `ORBIT_RELEASE_TRUSTED_KEYS_FILE`).
pub fn stage_bundled_bwrap_for_this_release() -> Result<StagedBwrap, OrbitError> {
    let asset = bundled_bwrap_asset_name(std::env::consts::ARCH).ok_or_else(|| {
        OrbitError::Execution(format!(
            "Orbit publishes no bundled Bubblewrap for {}",
            std::env::consts::ARCH
        ))
    })?;
    stage_bundled_bwrap(
        release_source_from_env().as_ref(),
        env!("CARGO_PKG_VERSION"),
        &asset,
        super::trust::trusted_keys_from_env()?,
        chrono::Utc::now().date_naive(),
    )
}

fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), OrbitError> {
    let stage_error = |error: std::io::Error| {
        OrbitError::Io(format!(
            "failed to stage the bundled Bubblewrap at '{}': {error}",
            path.display()
        ))
    };
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o755);
    }
    let mut file = options.open(path).map_err(stage_error)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(stage_error)
}
