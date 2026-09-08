//! Integrity verification for published Orbit release artifacts.
//!
//! Every consumer that installs a released binary — `install.sh`, the npm
//! installer, `orbit semantic install`, and `orbit update` — answers the same
//! two questions: *was this checksum manifest signed by a trusted release
//! key*, and *does this asset hash to the digest that manifest publishes*.
//!
//! This module owns the Rust answer to both, plus the trusted key set itself,
//! so a key rotation is one edit in Rust rather than one per consumer
//! (L-0044). It is deliberately pure: no network, no filesystem, no clock
//! reading. Callers supply the bytes and the date, which is also what makes
//! expiry and revocation testable without freezing wall-clock time.

use chrono::NaiveDate;
use rsa::RsaPublicKey;
use rsa::pkcs1v15::{Signature as RsaSignature, VerifyingKey};
use rsa::pkcs8::DecodePublicKey;
use rsa::sha2::Sha256 as RsaSha256;
use rsa::signature::Verifier;
use sha2::{Digest, Sha256};

use crate::OrbitError;

/// Name of the checksum manifest published with every Orbit release.
pub const RELEASE_CHECKSUMS_FILENAME: &str = "orbit-checksums.txt";

/// Name of the detached signature over [`RELEASE_CHECKSUMS_FILENAME`].
pub const RELEASE_CHECKSUMS_SIGNATURE_FILENAME: &str = "orbit-checksums.txt.sig";

/// One release signing key the installers trust.
///
/// `id` is a stable label with a generation counter, not a date: it survives
/// rotation so a key that has been the successor for a year is still readable
/// by ID. `not_after` and `revoked_at` are `YYYY-MM-DD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedReleaseKey {
    /// Stable key label, e.g. `orbit-release-key-3`.
    pub id: &'static str,
    /// Last day this key may verify a release, inclusive.
    pub not_after: &'static str,
    /// Set once a key is withdrawn; a matching signature then fails closed.
    pub revoked_at: Option<&'static str>,
    /// PKCS#8 SubjectPublicKeyInfo PEM block.
    pub public_key_pem: &'static str,
}

/// The current release signing key. Keep this block byte-identical to
/// `npm/release-signing.pub`; `scripts/check-installer-pubkey.sh` fails the
/// build if any consumer drifts.
const ORBIT_RELEASE_KEY_3_PEM: &str = r#"-----BEGIN PUBLIC KEY-----
MIIBojANBgkqhkiG9w0BAQEFAAOCAY8AMIIBigKCAYEAoQGLKOvvsvXriGIQ0oxA
PcDyVHLM1iqXBCYXg+blQU41haEkG1eYabvDfeGcyGaC4awW7Q2uCZK05+/Hdjpe
cRUVxP+QWKCAHyretQwOsoXzutZjJgId/ZRiUJPS/FeJOSv0xrayaol0tmfeJ4mH
gFseCLq+mIIWIPRvXmYiKaUB//bjF79w/m4VXlyBhfi6n+f6x2UPG+gjjsjwG6mn
Orec31AAFCIIX69YAd21D3MBc4S89/LoYZCq3neDscZ09Y+e6Jg2HpoBstvqSnq/
3s34unLuIRlyB8jyK8CrdzT1E6YVB7+riAjycE9XMlLOQ2xA4tl6CKIx5YTKHyeW
npMLlbzNaVfFT7p3IPTxsoEI0SB3ZtO7/XhzuOvOpklYcqjW2DGw/yzr2epAqHE/
y4rLO3hkxWhxfgF5KPSR2iftc3LMONRGWELK6jpD5KB7No5vwIvjpVPUc5xA45Xw
tT/bo0mm4TvrumxYr1xyEHrdum+ej/WYz/0BZQlwDOtXAgMBAAE=
-----END PUBLIC KEY-----"#;

/// The release signing keys this binary trusts.
///
/// Kept in lockstep with the same list in `install.sh` and
/// `npm/scripts/install-binary.js`.
pub const TRUSTED_RELEASE_KEYS: &[TrustedReleaseKey] = &[TrustedReleaseKey {
    id: "orbit-release-key-3",
    not_after: "2029-12-31",
    revoked_at: None,
    public_key_pem: ORBIT_RELEASE_KEY_3_PEM,
}];

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Normalize a user- or manifest-supplied digest to lowercase hex, rejecting
/// anything that is not exactly 64 hex characters.
///
/// `label` names the source in the rejection so the caller's own wording
/// ("`ORBIT_SEARCH_COMPANION_SHA256`", "release checksum manifest entry")
/// reaches the operator.
pub fn normalize_sha256(value: &str, label: &str) -> Result<String, OrbitError> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(OrbitError::InvalidInput(format!(
            "{label} must be a 64-character hex SHA-256 digest"
        )));
    }
    Ok(normalized)
}

/// Look up `asset_name`'s digest in a `sha256sum`-style checksum manifest.
///
/// Release manifests are generated in different working directories across
/// build jobs, so an entry may be bare (`orbit-x86_64-unknown-linux-gnu.tar.gz`)
/// or path-qualified (`./dist/orbit-…tar.gz`). Both match on file name.
pub fn checksum_for_asset(manifest: &str, asset_name: &str) -> Result<String, OrbitError> {
    for line in manifest.lines() {
        let mut fields = line.split_whitespace();
        let (Some(checksum), Some(name)) = (fields.next(), fields.next()) else {
            continue;
        };
        if manifest_name_matches(name, asset_name) {
            return normalize_sha256(checksum, "release checksum manifest entry");
        }
    }
    Err(OrbitError::Execution(format!(
        "checksum entry for release asset `{asset_name}` was not found in {RELEASE_CHECKSUMS_FILENAME}"
    )))
}

fn manifest_name_matches(name: &str, asset_name: &str) -> bool {
    name == asset_name
        || std::path::Path::new(name)
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .is_some_and(|file_name| file_name == asset_name)
}

/// Compare an observed digest against the expected one.
pub fn verify_sha256_digest(actual: &str, expected: &str, label: &str) -> Result<(), OrbitError> {
    let expected = normalize_sha256(expected, label)?;
    if actual != expected {
        return Err(OrbitError::Execution(format!(
            "{label} checksum verification failed (expected {expected}, got {actual})"
        )));
    }
    Ok(())
}

/// Verify a detached PKCS#1 v1.5 SHA-256 signature over `manifest` against one
/// specific public key.
///
/// Exposed separately from [`verify_checksum_signature`] so tests can pin a
/// throwaway keypair without mutating the trusted set.
pub fn verify_checksum_signature_with_key(
    manifest: &[u8],
    signature: &[u8],
    public_key_pem: &str,
) -> Result<(), OrbitError> {
    let public_key = RsaPublicKey::from_public_key_pem(public_key_pem).map_err(|error| {
        OrbitError::Execution(format!(
            "failed to load trusted release checksum signing key: {error}"
        ))
    })?;
    let signature = RsaSignature::try_from(signature).map_err(|error| {
        OrbitError::Execution(format!(
            "release checksum signature verification failed for {RELEASE_CHECKSUMS_FILENAME}: {error}"
        ))
    })?;
    VerifyingKey::<RsaSha256>::new(public_key)
        .verify(manifest, &signature)
        .map_err(|error| {
            OrbitError::Execution(format!(
                "release checksum signature verification failed for {RELEASE_CHECKSUMS_FILENAME}: {error}"
            ))
        })
}

/// Verify `signature` against `keys` and return the ID of the key that matched.
///
/// Expiry and revocation are checked *after* a key verifies, not before, so an
/// artifact signed by a withdrawn key is reported as exactly that rather than
/// as a generic "no trusted key matched". `today` is supplied by the caller;
/// this module never reads the clock.
pub fn verify_checksum_signature(
    manifest: &[u8],
    signature: &[u8],
    keys: &'static [TrustedReleaseKey],
    today: NaiveDate,
) -> Result<&'static str, OrbitError> {
    for key in keys {
        if verify_checksum_signature_with_key(manifest, signature, key.public_key_pem).is_err() {
            continue;
        }
        if let Some(revoked_at) = key.revoked_at {
            return Err(OrbitError::Execution(format!(
                "release checksum signature was made by revoked release signing key {} (revoked {revoked_at})",
                key.id
            )));
        }
        let not_after = parse_key_date(key.not_after, key.id)?;
        if today > not_after {
            return Err(OrbitError::Execution(format!(
                "release checksum signature was made by expired release signing key {} (not_after {})",
                key.id, key.not_after
            )));
        }
        return Ok(key.id);
    }
    Err(OrbitError::Execution(format!(
        "release checksum signature verification failed for {RELEASE_CHECKSUMS_FILENAME}: no trusted release signing key matched"
    )))
}

fn parse_key_date(value: &str, key_id: &str) -> Result<NaiveDate, OrbitError> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|error| {
        OrbitError::Execution(format!(
            "trusted release signing key {key_id} has an invalid date '{value}': {error}"
        ))
    })
}
