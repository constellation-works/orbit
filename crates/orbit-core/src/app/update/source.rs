//! Release transport port and bounded input helpers.

use orbit_common::OrbitError;
use orbit_common::security::release::{
    RELEASE_CHECKSUMS_FILENAME, RELEASE_CHECKSUMS_SIGNATURE_FILENAME,
};
use std::fmt::Debug;
use std::io::Read;

/// Input ceilings apply before authentication and before any complete artifact is buffered.
pub const MAX_METADATA_BYTES: u64 = 64 * 1024;

/// GitHub release JSON includes the release notes, unlike the small mirror marker.
pub const MAX_RELEASE_METADATA_BYTES: u64 = 256 * 1024;

/// Maximum checksum manifest size.
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Maximum checksum signature size.
pub const MAX_SIGNATURE_BYTES: u64 = 16 * 1024;

/// Maximum release archive and extracted executable size.
pub const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;

/// A source of published Orbit release artifacts.
pub trait ReleaseSource: Debug + Send + Sync {
    /// Where these artifacts come from, for diagnostics and reports.
    fn describe(&self) -> String;

    /// The newest published version, without a leading `v`.
    fn latest_version(&self) -> Result<String, OrbitError>;

    /// Unpack an authenticated release archive.
    fn unpack(&self, archive: &[u8], asset: &str) -> Result<Vec<u8>, OrbitError>;

    /// Fetch `asset` as published with `version`.
    fn fetch(&self, version: &str, asset: &str) -> Result<Vec<u8>, OrbitError>;
}

/// Size ceiling for a release asset.
pub fn asset_limit(asset: &str) -> u64 {
    match asset {
        RELEASE_CHECKSUMS_FILENAME => MAX_MANIFEST_BYTES,
        RELEASE_CHECKSUMS_SIGNATURE_FILENAME => MAX_SIGNATURE_BYTES,
        _ => MAX_ARCHIVE_BYTES,
    }
}

/// Construct the common oversized-input error.
pub fn too_large(what: &str, limit: u64) -> OrbitError {
    OrbitError::Execution(format!(
        "{what} exceeds the {limit}-byte release input limit"
    ))
}

/// Read an input up to its size ceiling.
pub fn read_bounded(reader: impl Read, what: &str, limit: u64) -> Result<Vec<u8>, OrbitError> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| OrbitError::Execution(format!("failed to read {what}: {error}")))?;
    if bytes.len() as u64 > limit {
        return Err(too_large(what, limit));
    }
    Ok(bytes)
}
