//! Process-level release trust set for `orbit update`.
//!
//! The compiled trust set is the default. `ORBIT_RELEASE_TRUSTED_KEYS_FILE`
//! replaces it only after `ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE=1`,
//! using the same `id|not_after|revoked_at|public_key_path` records as
//! `install.sh` and the npm installer. The deprecated single-key override is
//! not honored: a variable meant for those installers must not silently change
//! what `orbit update` trusts, and setting both is refused.

use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use orbit_common::OrbitError;
use orbit_common::security::release::{TRUSTED_RELEASE_KEYS, TrustedReleaseKey};

const TRUSTED_KEYS_FILE_ENV: &str = "ORBIT_RELEASE_TRUSTED_KEYS_FILE";
const TRUSTED_KEYS_ACK_ENV: &str = "ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE";
const PUBLIC_KEY_FILE_ENV: &str = "ORBIT_RELEASE_PUBLIC_KEY_FILE";

/// Same ceiling as other release metadata. A trust file is a handful of records.
const MAX_TRUST_FILE_BYTES: u64 = 64 * 1024;
const MAX_PEM_BYTES: u64 = 64 * 1024;

/// Keys this process should accept for a release manifest.
///
/// The returned slice is `'static` because signature verification holds the
/// trust set for the rest of the process. An override is leaked once per
/// process; `orbit update` loads it a single time.
pub(super) fn trusted_keys_from_env() -> Result<&'static [TrustedReleaseKey], OrbitError> {
    let public_override =
        std::env::var_os(PUBLIC_KEY_FILE_ENV).is_some_and(|value| !value.is_empty());
    let keys_file = std::env::var_os(TRUSTED_KEYS_FILE_ENV).filter(|value| !value.is_empty());
    if public_override && keys_file.is_some() {
        return Err(OrbitError::InvalidInput(format!(
            "{PUBLIC_KEY_FILE_ENV} and {TRUSTED_KEYS_FILE_ENV} cannot both be set"
        )));
    }
    let Some(keys_file) = keys_file else {
        return Ok(TRUSTED_RELEASE_KEYS);
    };
    if std::env::var(TRUSTED_KEYS_ACK_ENV).ok().as_deref() != Some("1") {
        return Err(OrbitError::InvalidInput(format!(
            "{TRUSTED_KEYS_FILE_ENV} requires {TRUSTED_KEYS_ACK_ENV}=1"
        )));
    }
    let path = PathBuf::from(keys_file);
    tracing::warn!(
        path = %path.display(),
        "{TRUSTED_KEYS_FILE_ENV} is set; trusting the replacement release signing key set"
    );
    let keys = load_trust_file(&path)?;
    Ok(Box::leak(keys.into_boxed_slice()))
}

fn load_trust_file(path: &Path) -> Result<Vec<TrustedReleaseKey>, OrbitError> {
    let bytes = read_capped(
        path,
        "trusted release signing key file",
        MAX_TRUST_FILE_BYTES,
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "trusted release signing key file '{}' is not UTF-8: {error}",
            path.display()
        ))
    })?;
    let manifest_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut keys = Vec::new();
    for (line_number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        keys.push(parse_record(line, line_number + 1, manifest_dir)?);
    }
    if keys.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "trusted release signing key file '{}' contains no keys",
            path.display()
        )));
    }
    Ok(keys)
}

fn parse_record(
    line: &str,
    line_number: usize,
    manifest_dir: &Path,
) -> Result<TrustedReleaseKey, OrbitError> {
    let fields: Vec<&str> = line.split('|').collect();
    if fields.len() != 4 {
        return Err(OrbitError::InvalidInput(format!(
            "invalid trusted release signing key record on line {line_number}: expected id|not_after|revoked_at|public_key_path"
        )));
    }
    let id = fields[0].trim();
    let not_after = fields[1].trim();
    let revoked_at = fields[2].trim();
    let public_key_field = fields[3].trim();
    if id.is_empty() || public_key_field.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "trusted release signing key record on line {line_number} is missing an id or public key path"
        )));
    }
    require_date(not_after, id, "not_after")?;
    if !revoked_at.is_empty() {
        require_date(revoked_at, id, "revoked_at")?;
    }
    let key_path = Path::new(public_key_field);
    let resolved = if key_path.is_absolute() {
        key_path.to_path_buf()
    } else {
        manifest_dir.join(key_path)
    };
    let pem_bytes = read_capped(&resolved, "trusted release public key", MAX_PEM_BYTES)?;
    let pem = String::from_utf8(pem_bytes).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "trusted release public key '{}' is not UTF-8: {error}",
            resolved.display()
        ))
    })?;
    Ok(TrustedReleaseKey {
        id: leak_str(id),
        not_after: leak_str(not_after),
        revoked_at: (!revoked_at.is_empty()).then(|| leak_str(revoked_at)),
        public_key_pem: leak_str(pem),
    })
}

fn require_date(value: &str, key_id: &str, field: &str) -> Result<(), OrbitError> {
    if NaiveDate::parse_from_str(value, "%Y-%m-%d").is_err() {
        return Err(OrbitError::InvalidInput(format!(
            "trusted release signing key {key_id} has an invalid {field} '{value}' (expected YYYY-MM-DD)"
        )));
    }
    Ok(())
}

fn read_capped(path: &Path, what: &str, limit: u64) -> Result<Vec<u8>, OrbitError> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        OrbitError::Io(format!(
            "failed to read {what} at '{}': {error}",
            path.display()
        ))
    })?;
    let len = file
        .metadata()
        .map_err(|error| {
            OrbitError::Io(format!(
                "failed to inspect {what} at '{}': {error}",
                path.display()
            ))
        })?
        .len();
    if len > limit {
        return Err(OrbitError::InvalidInput(format!(
            "{what} at '{}' exceeds the {limit}-byte limit",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|error| {
        OrbitError::Io(format!(
            "failed to read {what} at '{}': {error}",
            path.display()
        ))
    })?;
    if bytes.len() as u64 > limit {
        return Err(OrbitError::InvalidInput(format!(
            "{what} at '{}' exceeds the {limit}-byte limit",
            path.display()
        )));
    }
    Ok(bytes)
}

fn leak_str(value: impl Into<String>) -> &'static str {
    Box::leak(value.into().into_boxed_str())
}
