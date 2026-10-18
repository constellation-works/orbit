//! The release signing keys the app trusts, and manifest verification.
//!
//! A copy of `orbit-common::security::release`'s trust set: this app is its
//! own Cargo workspace and depends on no Orbit crate. Every key's id,
//! `not_after`, `revoked_at` and PEM must match that set byte for byte;
//! `scripts/check-installer-pubkey.sh` fails CI when they drift. Rotation
//! edits both, as `docs/runbooks/release.md` describes.

use chrono::NaiveDate;
use rsa::RsaPublicKey;
use rsa::pkcs1v15::{Signature, VerifyingKey};
use rsa::pkcs8::DecodePublicKey;
use rsa::signature::Verifier;
use sha2::{Digest, Sha256};

/// One release signing key. Dates are `YYYY-MM-DD`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TrustedReleaseKey {
    /// Stable key label, e.g. `orbit-release-key-3`.
    pub(crate) id: &'static str,
    /// Last day this key may verify a release, inclusive.
    pub(crate) not_after: &'static str,
    /// Set once a key is withdrawn; a matching signature then fails closed.
    pub(crate) revoked_at: Option<&'static str>,
    /// PKCS#8 SubjectPublicKeyInfo PEM block.
    pub(crate) public_key_pem: &'static str,
}

/// Keep byte-identical to the same constant in
/// `crates/orbit-common/src/security/release.rs`.
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

/// The release signing keys this app trusts.
pub(crate) const TRUSTED_RELEASE_KEYS: &[TrustedReleaseKey] = &[TrustedReleaseKey {
    id: "orbit-release-key-3",
    not_after: "2029-12-31",
    revoked_at: None,
    public_key_pem: ORBIT_RELEASE_KEY_3_PEM,
}];

/// Verify a detached PKCS#1 v1.5 SHA-256 signature over `manifest` and return
/// the ID of the key that made it.
///
/// Expiry and revocation are checked after a key verifies, so a manifest
/// signed by a withdrawn key is reported as exactly that.
pub(crate) fn verify_manifest(
    manifest: &[u8],
    signature: &[u8],
    keys: &[TrustedReleaseKey],
    today: NaiveDate,
) -> Result<&'static str, String> {
    let signature = Signature::try_from(signature)
        .map_err(|error| format!("the checksum signature is malformed: {error}"))?;
    for key in keys {
        let Ok(public_key) = RsaPublicKey::from_public_key_pem(key.public_key_pem) else {
            continue;
        };
        if VerifyingKey::<Sha256>::new(public_key)
            .verify(manifest, &signature)
            .is_err()
        {
            continue;
        }
        if let Some(revoked_at) = key.revoked_at {
            return Err(format!(
                "the checksums were signed by revoked release key {} (revoked {revoked_at})",
                key.id
            ));
        }
        let not_after = NaiveDate::parse_from_str(key.not_after, "%Y-%m-%d")
            .map_err(|error| format!("release key {} has an invalid not_after: {error}", key.id))?;
        if today > not_after {
            return Err(format!(
                "the checksums were signed by expired release key {} (not_after {})",
                key.id, key.not_after
            ));
        }
        return Ok(key.id);
    }
    Err("no trusted release key matched the checksum signature".into())
}

/// The lowercase hex SHA-256 that a `sha256sum`-style `manifest` lists for
/// `asset`.
pub(crate) fn checksum_for(manifest: &str, asset: &str) -> Option<String> {
    manifest.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let digest = fields.next()?;
        let name = fields.next()?;
        let name = name.trim_start_matches('*');
        let name = name.rsplit('/').next().unwrap_or(name);
        let digest = digest.to_ascii_lowercase();
        (name == asset && digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then_some(digest)
    })
}

/// Lowercase hex of a finished SHA-256.
pub(crate) fn hex(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Lowercase hex SHA-256 of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
