use std::io::Read;

use orbit_common::OrbitError;
use orbit_core::app::update::source::MAX_ARCHIVE_BYTES;

const ARCHIVE_MEMBER: &str = "orbit";

/// Extract the single `orbit` member from a release archive.
///
/// Mirrors the archive checks in `install.sh`: exactly one member, named
/// `orbit`, a regular file, with no path separator or traversal in its name.
/// A release archive is authenticated by this point, so these are defense in
/// depth against a signing-side mistake rather than the primary control.
pub(super) fn extract_release_executable(
    archive: &[u8],
    asset: &str,
) -> Result<Vec<u8>, OrbitError> {
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
